//! Measures how long two endpoints on the same network take to start talking DIRECTLY.
//!
//! The product's promise is that a phone and a Mac in the same room respond near-instantly. They
//! do not start out that way. `presets::N0` configures pkarr publish + DNS resolve + the n0 relay
//! fleet and nothing else; `PkarrPublisher` defaults to publishing relay addresses only; and the
//! ticket the app stores is node-id-only. Resolving that node id therefore yields a relay URL, so
//! the connection STARTS relayed -- every packet crossing the room via a datacentre -- and only
//! becomes direct once in-band NAT-traversal candidate exchange has discovered the local
//! addresses for itself.
//!
//! mDNS address lookup hands over those local addresses immediately, and does it without
//! publishing home IP addresses to a public DHT, because mDNS records never leave the local link.
//!
//! Scenario one isolates the mechanism: `presets::Minimal` has no relay and no DNS at all, so a
//! connection can ONLY happen if mDNS resolved it. Scenario two is the product question, run
//! twice -- stock N0 against N0 + mDNS -- measuring time to a SELECTED DIRECT path, which is what
//! the user actually feels.
//!
//! Needs a real network interface and macOS local-network permission, so this is a local probe,
//! not a CI one.

use std::time::{Duration, Instant};

use iroh_mdns_address_lookup::MdnsAddressLookup;

const PROBE_ALPN: &[u8] = b"holoiroh/lan-discovery-probe/1";

/// Compile-time copies of every file that has to keep this wired up. The measurement below runs
/// against endpoints this probe builds itself, so it would keep passing even if the daemon, the
/// bridge, or the app's Info.plist stopped opting in -- exactly the silent regression that would
/// put every same-room session back on the relay with nothing to notice it.
const WIRING_SOURCES: &[(&str, &str)] = &[
    ("mac-daemon/src/main.rs", include_str!("../src/main.rs")),
    (
        "ios-bridge/src/lib.rs",
        include_str!("../../ios-bridge/src/lib.rs"),
    ),
];
const INFO_PLIST: &str = include_str!("../../ios-app/Info.plist");
const PROJECT_YML: &str = include_str!("../../ios-app/project.yml");
const CONNECT_BUDGET: Duration = Duration::from_secs(20);
const DIRECT_BUDGET: Duration = Duration::from_secs(25);
/// Stock N0 resolves a node id through pkarr publish -> n0 DNS, which is not instant: dialing a
/// freshly-bound endpoint fails outright with "Failed to resolve TXT record". Both N0 scenarios
/// get the same settle window so the comparison is fair, and it is excluded from the timings.
const PKARR_SETTLE: Duration = Duration::from_secs(8);

fn selected_path(connection: &iroh::endpoint::Connection) -> &'static str {
    match connection.paths().iter().find(|p| p.is_selected()) {
        Some(path) if path.is_relay() => "relay",
        Some(_) => "direct",
        None => "unknown",
    }
}

struct Outcome {
    connected_in: Duration,
    direct_after: Option<Duration>,
    path_at_connect: &'static str,
}

/// Dials by node id ALONE -- no relay URL, no IP hints. Whatever resolves the address has to have
/// found it, which is the whole point.
async fn dial_by_id_only(with_mdns: bool, minimal: bool) -> anyhow::Result<Outcome> {
    let build = |alpns: Option<Vec<Vec<u8>>>| {
        let mut builder = if minimal {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        } else {
            iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        };
        if let Some(alpns) = alpns {
            builder = builder.alpns(alpns);
        }
        if with_mdns {
            // The builder's default AddrFilter is the identity filter, so local IP addresses are
            // published as-is. That is exactly what is wanted here and is safe: an mDNS record is
            // link-local, unlike the public DHT the pkarr publisher writes to.
            builder = builder.address_lookup(MdnsAddressLookup::builder());
        }
        builder
    };

    let listener = build(Some(vec![PROBE_ALPN.to_vec()])).bind().await?;
    let listener_id = listener.id();
    let dialer = build(None).bind().await?;

    let accept_task = tokio::spawn(async move {
        let Some(incoming) = listener.accept().await else {
            return;
        };
        if let Ok(connection) = incoming.await {
            // Hold the connection open so the dialer's path can be validated and upgraded.
            tokio::time::sleep(DIRECT_BUDGET).await;
            drop(connection);
        }
    });

    if !minimal {
        tokio::time::sleep(PKARR_SETTLE).await;
    }

    let bare = iroh::EndpointAddr::new(listener_id);
    anyhow::ensure!(
        bare.addrs.is_empty(),
        "the dial target must carry no address hints, or this proves nothing"
    );

    let started = Instant::now();
    let connection = tokio::time::timeout(CONNECT_BUDGET, dialer.connect(bare, PROBE_ALPN))
        .await
        .map_err(|_| anyhow::anyhow!("no connection within {CONNECT_BUDGET:?}"))??;
    let connected_in = started.elapsed();
    let path_at_connect = selected_path(&connection);

    let mut direct_after = None;
    let deadline = Instant::now() + DIRECT_BUDGET;
    while Instant::now() < deadline {
        if selected_path(&connection) == "direct" {
            direct_after = Some(started.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    accept_task.abort();
    Ok(Outcome {
        connected_in,
        direct_after,
        path_at_connect,
    })
}

fn describe(label: &str, outcome: &Outcome) {
    match outcome.direct_after {
        Some(direct) => println!(
            "  {label:<24} connected in {:.2?} on the {} path, DIRECT after {:.2?}",
            outcome.connected_in, outcome.path_at_connect, direct
        ),
        None => println!(
            "  {label:<24} connected in {:.2?} on the {} path, never became direct within {DIRECT_BUDGET:?}",
            outcome.connected_in, outcome.path_at_connect
        ),
    }
}

/// The source guards are pure compile-time string checks and run everywhere, including CI. The
/// measurement needs a real network interface, multicast, and macOS local-network permission, so
/// it runs only when explicitly asked for -- but the guards are the part that would otherwise
/// rot, since nothing else notices if the daemon quietly stops opting in.
fn measure_the_network() -> bool {
    std::env::var("HOLOIROH_LAN_PROBE_NETWORK").as_deref() == Ok("1")
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    if !measure_the_network() {
        check_the_product_opts_in()?;
        println!(
            "\nVERDICT: OK -- every side still opts into local-network discovery. The timing \
             measurement needs multicast and local-network permission; set \
             HOLOIROH_LAN_PROBE_NETWORK=1 on a real machine to run it."
        );
        return Ok(());
    }

    println!("does mDNS resolve an endpoint at all? (no relay, no DNS, no address hints)");
    let isolated = dial_by_id_only(true, true).await?;
    describe("Minimal + mDNS", &isolated);
    anyhow::ensure!(
        isolated.path_at_connect == "direct",
        "with no relay configured the only possible path is direct; got {}",
        isolated.path_at_connect
    );
    println!("  -> mDNS alone is enough to find and directly reach a peer by node id");

    println!(
        "\nhow long until two endpoints in the same room talk DIRECTLY? \
         (both given {PKARR_SETTLE:?} to publish first, excluded from the timings)"
    );
    let stock = match dial_by_id_only(false, false).await {
        Ok(outcome) => {
            describe("N0 (what ships today)", &outcome);
            Some(outcome)
        }
        Err(err) => {
            // Not fatal, and not noise: failing to resolve a peer that mDNS found in a third of a
            // second is the finding, so record it and carry on to the mDNS run.
            println!("  N0 (what ships today)   could not resolve the peer at all: {err:#}");
            None
        }
    };
    let with_mdns = dial_by_id_only(true, false).await?;
    describe("N0 + mDNS", &with_mdns);

    match (stock.as_ref().and_then(|s| s.direct_after), with_mdns.direct_after) {
        (Some(a), Some(b)) if b < a => println!(
            "\nmDNS reached a direct path {:.2?} sooner ({a:.2?} -> {b:.2?})",
            a - b
        ),
        (None, Some(b)) => println!(
            "\nstock never reached a direct path within {DIRECT_BUDGET:?}; with mDNS it took {b:.2?}"
        ),
        (Some(a), Some(b)) => println!(
            "\nno improvement measured this run (stock {a:.2?} vs mDNS {b:.2?}) -- on a link where \
             NAT traversal already succeeds quickly there is little left for mDNS to win"
        ),
        (_, None) => println!(
            "\nmDNS did NOT reach a direct path within {DIRECT_BUDGET:?}, which contradicts the \
             isolated scenario above and needs explaining before wiring this in"
        ),
    }

    anyhow::ensure!(
        with_mdns.direct_after.is_some(),
        "with mDNS configured the endpoints must end up on a direct path"
    );

    check_the_product_opts_in()?;

    println!(
        "\nVERDICT: OK -- mDNS finds a peer by node id alone with no relay and no address hints, \
         and leaves the pair on a direct path. It is additive to N0 rather than a replacement, so \
         connecting from outside the LAN still works through the relay exactly as before."
    );
    Ok(())
}

fn check_the_product_opts_in() -> anyhow::Result<()> {
    println!("the product actually opts in");
    for (path, source) in WIRING_SOURCES {
        anyhow::ensure!(
            source.contains("MdnsAddressLookup::builder()"),
            "{path} no longer registers the mDNS address lookup, so its endpoint is back to \
             starting every same-room session on the relay"
        );
        println!("  ok   {path} registers the mDNS address lookup");
    }
    anyhow::ensure!(
        INFO_PLIST.contains("NSBonjourServices") && INFO_PLIST.contains("_irohv1._udp"),
        "ios-app/Info.plist does not declare the Bonjour service type; iOS refuses the multicast \
         queries silently, so discovery returns nothing and the session quietly starts relayed"
    );
    println!("  ok   Info.plist declares _irohv1._udp");
    anyhow::ensure!(
        PROJECT_YML.contains("NSBonjourServices"),
        "ios-app/project.yml would regenerate an Info.plist without NSBonjourServices, undoing \
         the entry above on the next xcodegen run"
    );
    anyhow::ensure!(
        !PROJECT_YML.contains("NSBonjourServices is not required"),
        "project.yml still carries the comment claiming NSBonjourServices is not required. It was \
         true before mDNS was added and is now the opposite of the truth -- a maintainer reading \
         it removes the key and silently sends every same-room session back through the relay"
    );
    println!("  ok   project.yml declares it, and its stale contradicting comment is gone");
    Ok(())
}

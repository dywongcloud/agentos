//! Measures how long a cursor move takes to become a cursor move, over a real iroh connection.
//!
//! Everything else in this pass measures one hop in isolation: `injection_cost_probe` times the
//! injection, `MoveCoalescingCheck` times the send policy, `remote_input_ordering_probe` checks
//! ordering. None of them answers the question the user actually asked -- when the two devices
//! are near each other, how long is it between the finger moving and the cursor moving.
//!
//! This runs both ends in one process so both timestamps come from one clock: a real
//! `iroh::Endpoint` pair, the real `CONTROL_ALPN`, the real `TaskEnvelope` NDJSON framing, the
//! real `read_line` loop, and the real `remote_input` injection under `HOLOIROH_INPUT_DRY_RUN=1`.
//! Moves are sent at display cadence rather than in a burst, because a burst measures queue
//! drain rather than responsiveness.
//!
//! Loopback is faster than a phone on Wi-Fi, so the absolute numbers are a FLOOR, not a
//! prediction. What they bound is everything the daemon controls: framing, decode, dispatch and
//! injection. Anything beyond this floor on a real link is the network and the video path.

use std::time::{Duration, Instant};

use holoiroh_daemon::remote_input;
use holoiroh_wire::{CONTROL_ALPN, ClientMessage, RemoteControlEvent, TaskEnvelope};
use tokio::io::{AsyncBufReadExt, BufReader};

const MOVES: usize = 240;
const DISPLAY_CADENCE: Duration = Duration::from_micros(8_333);
/// Moves to discount as connection warmup when reporting steady state.
const WARMUP_MOVES: usize = 30;
/// Long enough that QUIC would have to recover from idle if it ever does.
const IDLE_GAP: Duration = Duration::from_secs(3);

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn envelope(sequence: u64, payload: ClientMessage) -> TaskEnvelope<ClientMessage> {
    TaskEnvelope {
        protocol_version: holoiroh_wire::PROTOCOL_VERSION,
        message_id: format!("probe-{sequence}"),
        session_id: "probe-session".to_string(),
        task_id: None,
        message_type: "client".to_string(),
        sent_at: 0,
        expires_at: u64::MAX,
        sequence_number: sequence,
        payload,
        signature: None,
    }
}

/// `HOLOIROH_PROBE_INITIAL_RTT_MS` exists because the opening moves of a fresh connection cost
/// tens of milliseconds, and stock QUIC assumes a 333ms RTT until it takes its first sample --
/// an obvious-looking culprit.
///
/// It is not the culprit. Measured three runs each at stock, 10ms and 50ms: the first move cost
/// 25-33ms in every configuration, and steady-state p50 stayed at 135-208us throughout. The
/// opening cost is connection and stream establishment itself, and the probe's second drag
/// already shows it does not recur after an idle period. The hook is kept precisely so this
/// stays measured rather than being re-proposed from first principles.
fn probe_transport_config() -> iroh::endpoint::QuicTransportConfig {
    let builder = iroh::endpoint::QuicTransportConfig::builder();
    match std::env::var("HOLOIROH_PROBE_INITIAL_RTT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(ms) => {
            println!("initial_rtt overridden to {ms}ms");
            builder.initial_rtt(Duration::from_millis(ms)).build()
        }
        None => {
            println!("initial_rtt left at the stock default");
            builder.build()
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    assert_eq!(
        std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref(),
        Ok("1"),
        "this probe must run with HOLOIROH_INPUT_DRY_RUN=1 so it records moves instead of \
         driving the machine's real cursor"
    );

    // `presets::Minimal` rather than `N0`: no relay, no DNS/pkarr lookup, and the address is
    // handed over directly as a loopback socket. That is deliberately the BEST case -- two
    // endpoints already on a direct path -- because the point is to floor the daemon's own cost,
    // not to measure how long iroh takes to find a peer.
    let transport = probe_transport_config();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![CONTROL_ALPN.to_vec()])
        .transport_config(transport.clone())
        .bind()
        .await?;
    let sockets = server.bound_sockets();
    println!("server bound sockets: {sockets:?}");
    let server_addr = iroh::EndpointAddr::from_parts(
        server.id(),
        sockets.into_iter().map(|mut s| {
            if s.ip().is_unspecified() {
                // Keep the family: bound_sockets() returns both 0.0.0.0 and [::], and rewriting
                // the v6 entry to 127.0.0.1 would pair an IPv4 address with the port the IPv6
                // socket is listening on.
                s.set_ip(if s.is_ipv6() {
                    std::net::Ipv6Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv4Addr::LOCALHOST.into()
                });
            }
            iroh::TransportAddr::Ip(s)
        }),
    );
    println!("dialing: {server_addr:?}");
    let client = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .transport_config(transport)
        .bind()
        .await?;

    let (applied_tx, mut applied_rx) = tokio::sync::mpsc::unbounded_channel::<(usize, Instant)>();

    let reader = tokio::spawn(async move {
        let incoming = server.accept().await.expect("no inbound connection");
        let connection = incoming.await.expect("handshake failed");
        let (_send, recv) = connection.accept_bi().await.expect("no bi stream");
        let mut lines = BufReader::new(recv).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(envelope) = serde_json::from_str::<TaskEnvelope<ClientMessage>>(&line) else {
                continue;
            };
            if let ClientMessage::RemoteControl {
                event: RemoteControlEvent::Move { x, y },
            } = envelope.payload
            {
                remote_input::move_cursor(x, y);
                if applied_tx
                    .send((envelope.sequence_number as usize, Instant::now()))
                    .is_err()
                {
                    break;
                }
            }
        }
    });

    let connection = client.connect(server_addr, CONTROL_ALPN).await?;
    let (mut send, _recv) = connection.open_bi().await?;

    let _ = remote_input::take_applied_moves();
    let mut sent_at = Vec::with_capacity(MOVES);
    for i in 0..MOVES {
        let t = i as f64 / MOVES as f64;
        let message = ClientMessage::RemoteControl {
            event: RemoteControlEvent::Move { x: t, y: t * 0.5 },
        };
        sent_at.push(Instant::now());
        holoiroh_wire::write_line(&mut send, &envelope(i as u64, message)).await?;
        tokio::time::sleep(DISPLAY_CADENCE).await;
    }

    let mut latencies = Vec::with_capacity(MOVES);
    let mut ordered_latencies = vec![Duration::ZERO; MOVES];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while latencies.len() < MOVES {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, applied_rx.recv()).await {
            Ok(Some((index, at))) => {
                let took = at.duration_since(sent_at[index]);
                latencies.push(took);
                ordered_latencies[index] = took;
            }
            _ => break,
        }
    }

    assert_eq!(
        latencies.len(),
        MOVES,
        "only {} of {MOVES} moves arrived; a harness that silently loses input would report a \
         flattering number",
        latencies.len()
    );

    let applied = remote_input::take_applied_moves();
    assert_eq!(
        applied.len(),
        MOVES,
        "the injection path saw {} moves, not {MOVES}",
        applied.len()
    );
    assert!(
        applied.windows(2).all(|w| w[1].0 >= w[0].0),
        "moves were applied out of order over a real connection, which would step the cursor \
         backwards"
    );

    let mut in_order: Vec<(usize, Duration)> = Vec::new();
    for (i, d) in ordered_latencies.iter().enumerate() {
        in_order.push((i, *d));
    }
    println!("first 12 moves, in send order:");
    for (i, d) in in_order.iter().take(12) {
        println!("  move {i:>3}  {d:.2?}");
    }
    let worst = in_order
        .iter()
        .max_by_key(|(_, d)| *d)
        .copied()
        .unwrap_or((0, Duration::ZERO));
    println!("slowest move was #{} at {:.2?}", worst.0, worst.1);

    let steady: Vec<Duration>;
    {
        let mut steady_tmp: Vec<Duration> = in_order
            .iter()
            .filter(|(i, _)| *i >= WARMUP_MOVES)
            .map(|(_, d)| *d)
            .collect();
        steady_tmp.sort();
        steady = steady_tmp;
        println!(
            "\nexcluding the first {WARMUP_MOVES} moves (connection warmup):\n  p50 {:.2?}   p90 {:.2?}   p99 {:.2?}   max {:.2?}",
            percentile(&steady, 0.50),
            percentile(&steady, 0.90),
            percentile(&steady, 0.99),
            steady.last().copied().unwrap_or_default(),
        );
    }

    latencies.sort();
    println!(
        "touch -> cursor over a real iroh connection ({MOVES} moves at display cadence)\n  \
         p50 {:.2?}   p90 {:.2?}   p99 {:.2?}   max {:.2?}",
        percentile(&latencies, 0.50),
        percentile(&latencies, 0.90),
        percentile(&latencies, 0.99),
        latencies.last().copied().unwrap_or_default(),
    );

    let frame = Duration::from_micros(16_667);
    // Asserting on a tail percentile would make this flaky rather than strict: it runs on
    // whatever else the machine is doing, and the p99/max above move by milliseconds between
    // runs on a busy box. The median is the stable statistic, and the tail is printed so a real
    // regression is still visible to a reader.
    let steady_p50 = percentile(&steady, 0.50);
    assert!(
        steady_p50 < Duration::from_millis(2),
        "median send-to-applied is {steady_p50:.2?} on a DIRECT link with no network delay; the \
         daemon's own framing/decode/dispatch/injection path has become a latency source in its \
         own right"
    );

    // The warmup above could be one-time connection setup, or it could be QUIC recovering from
    // an idle period -- which would mean the user pays it at the START OF EVERY DRAG, since the
    // control channel is silent between them. Those two possibilities look identical in the
    // numbers above and have completely different consequences, so ask directly.
    println!("\nidling the control channel for {IDLE_GAP:.1?}, then dragging again");
    tokio::time::sleep(IDLE_GAP).await;

    let mut second_round = vec![Duration::ZERO; MOVES];
    let mut second_sent = Vec::with_capacity(MOVES);
    for i in 0..MOVES {
        let t = i as f64 / MOVES as f64;
        let message = ClientMessage::RemoteControl {
            event: RemoteControlEvent::Move { x: t, y: t * 0.5 },
        };
        second_sent.push(Instant::now());
        holoiroh_wire::write_line(&mut send, &envelope((MOVES + i) as u64, message)).await?;
        tokio::time::sleep(DISPLAY_CADENCE).await;
    }
    let mut collected = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while collected < MOVES {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, applied_rx.recv()).await {
            Ok(Some((index, at))) if index >= MOVES => {
                let i = index - MOVES;
                second_round[i] = at.duration_since(second_sent[i]);
                collected += 1;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    println!("first 6 moves of the second drag:");
    for (i, d) in second_round.iter().take(6).enumerate() {
        println!("  move {i:>3}  {d:.2?}");
    }
    let mut second_sorted = second_round.clone();
    second_sorted.sort();
    println!(
        "second drag after idle:\n  p50 {:.2?}   p90 {:.2?}   p99 {:.2?}   max {:.2?}",
        percentile(&second_sorted, 0.50),
        percentile(&second_sorted, 0.90),
        percentile(&second_sorted, 0.99),
        second_sorted.last().copied().unwrap_or_default(),
    );

    let first_of_second = second_round[0];
    assert!(
        first_of_second < frame,
        "the first move of a drag that follows an idle control channel took {first_of_second:.2?}, \
         over a display frame. That is not one-time connection setup -- the user pays it every \
         time they start dragging after a pause, which is exactly what 'the cursor is slow to \
         respond' feels like"
    );

    drop(send);
    reader.abort();

    println!(
        "\nVERDICT: OK -- steady-state send-to-applied is {:.2?} p50 / {:.2?} p99 on a direct \
         link, no move arrives out of order, and starting a drag after an idle control channel \
         costs {first_of_second:.2?} rather than repeating the connection-setup cost. Tail \
         figures include whatever else the machine was running; the median is the number to \
         compare across runs.",
        percentile(&steady, 0.50),
        percentile(&steady, 0.99),
    );
    Ok(())
}
